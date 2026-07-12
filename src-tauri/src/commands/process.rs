//! Process detection commands

use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;

#[cfg(windows)]
use std::collections::HashSet;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const CODEX_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const CODEX_STOP_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[cfg(windows)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WindowsCodexProcess {
    #[serde(default)]
    name: String,
    process_id: u32,
    #[serde(default)]
    parent_process_id: u32,
    #[serde(default)]
    command_line: String,
    #[serde(default)]
    executable_path: String,
    #[serde(default)]
    main_window_title: String,
}

#[derive(Debug, Clone)]
struct ActiveCodexProcess {
    pid: u32,
    command_line: String,
    executable_path: Option<String>,
}

#[derive(Debug, Clone)]
struct CodexLaunchSpec {
    pid: u32,
    executable: String,
    args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CodexRestartPlan {
    processes: Vec<CodexLaunchSpec>,
}

impl CodexRestartPlan {
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }
}

/// Information about running Codex processes
#[derive(Debug, Clone, serde::Serialize)]
pub struct CodexProcessInfo {
    /// Number of active Codex app instances
    pub count: usize,
    /// Number of ignored background/stale Codex-related processes
    pub background_count: usize,
    /// Whether switching is allowed (no active Codex app instances)
    pub can_switch: bool,
    /// Process IDs of active Codex app instances
    pub pids: Vec<u32>,
}

/// Check for running Codex processes
#[tauri::command]
pub async fn check_codex_processes() -> Result<CodexProcessInfo, String> {
    let (pids, bg_count) = find_codex_processes().map_err(|e| e.to_string())?;
    let count = pids.len();

    Ok(CodexProcessInfo {
        count,
        background_count: bg_count,
        can_switch: count == 0,
        pids,
    })
}

/// Reject account switching when live Codex app instances are still running.
pub fn ensure_switch_allowed() -> anyhow::Result<()> {
    let (pids, _) = find_codex_processes()?;
    if pids.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "Close all running Codex windows before switching accounts. Active Codex processes: {}",
        pids.len()
    );
}

/// Capture the currently running Codex processes so they can be stopped and relaunched.
pub fn prepare_codex_restart_plan() -> anyhow::Result<CodexRestartPlan> {
    let (active_processes, _) = find_active_codex_processes()?;
    let processes = active_processes
        .into_iter()
        .map(build_launch_spec)
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(CodexRestartPlan { processes })
}

/// Stop the processes in a restart plan and wait until the switch guard clears.
pub fn stop_codex_for_restart(plan: &CodexRestartPlan) -> anyhow::Result<()> {
    if !plan.is_empty() {
        let (active_pids, _) = find_codex_processes()?;
        for process in &plan.processes {
            if active_pids.contains(&process.pid) {
                terminate_codex_process(process.pid)?;
            }
        }
    }

    wait_until_switch_guard_clears()
}

/// Relaunch the processes captured in a restart plan.
pub fn start_codex_from_restart_plan(plan: &CodexRestartPlan) -> anyhow::Result<()> {
    for process in &plan.processes {
        launch_codex_process(process)?;
    }

    Ok(())
}

/// Restart Antigravity/OpenAI extension app-server processes so they pick up auth changes.
pub fn restart_antigravity_background_processes() {
    if let Ok(pids) = find_antigravity_processes() {
        for pid in pids {
            if let Err(error) = terminate_antigravity_process(pid) {
                eprintln!("[process] failed to restart Antigravity process {pid}: {error:#}");
            }
        }
    }
}

/// Find all running codex processes. Returns (active_pids, background_count)
fn find_codex_processes() -> anyhow::Result<(Vec<u32>, usize)> {
    let (processes, background_count) = find_active_codex_processes()?;
    Ok((
        processes
            .into_iter()
            .map(|process| process.pid)
            .collect::<Vec<_>>(),
        background_count,
    ))
}

/// Find all running codex processes. Returns (active_processes, background_count)
fn find_active_codex_processes() -> anyhow::Result<(Vec<ActiveCodexProcess>, usize)> {
    #[cfg(test)]
    if let Ok(value) = std::env::var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT") {
        let count = value
            .parse::<usize>()
            .context("Invalid CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT")?;
        return Ok((
            (0..count)
                .map(|index| ActiveCodexProcess {
                    pid: index as u32 + 1,
                    command_line: "codex".to_string(),
                    executable_path: Some("codex".to_string()),
                })
                .collect(),
            0,
        ));
    }

    #[cfg(unix)]
    {
        let mut processes: Vec<ActiveCodexProcess> = Vec::new();
        let mut bg_count = 0;

        // Use ps with custom format to get the pid and full command line
        let output = Command::new("ps").args(["-eo", "pid,command"]).output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().skip(1) {
                // Skip header
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                // The first part is PID, the rest is the command string
                if let Some((pid_str, command)) = line.split_once(' ') {
                    let command = command.trim();

                    // Get the executable path/name (first word of the command string before args)
                    let executable = command.split_whitespace().next().unwrap_or("");

                    // Check if the executable is exactly "codex" or ends with "/codex"
                    let is_codex = executable == "codex" || executable.ends_with("/codex");

                    // Exclude if it's running from an extension or IDE integration (like Antigravity)
                    // These are expected background processes we shouldn't block on
                    let is_ide_plugin = is_ide_plugin_process(command);

                    // Skip our own app
                    let is_switcher =
                        command.contains("codex-switcher")
                            || command.contains("Codex Switcher")
                            || command.contains("ChatGPT Codex Switcher");

                    if is_codex && !is_switcher {
                        if let Ok(pid) = pid_str.trim().parse::<u32>() {
                            if pid != std::process::id()
                                && !processes.iter().any(|process| process.pid == pid)
                            {
                                if is_ide_plugin {
                                    bg_count += 1;
                                } else {
                                    processes.push(ActiveCodexProcess {
                                        pid,
                                        command_line: command.to_string(),
                                        executable_path: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        return Ok((processes, bg_count));
    }

    #[cfg(windows)]
    {
        return find_windows_active_codex_processes();
    }

    #[allow(unreachable_code)]
    Ok((Vec::new(), 0))
}

#[cfg(windows)]
fn find_windows_active_codex_processes() -> anyhow::Result<(Vec<ActiveCodexProcess>, usize)> {
    // tasklist counts every Electron helper (`--type=gpu-process`, crashpad, renderer, etc.),
    // which inflates the badge and incorrectly blocks switching. Use PowerShell so we can inspect
    // the command line and only count live top-level app instances.
    const POWERSHELL_SCRIPT: &str = r#"
$windowTitles = @{}
Get-Process -Name Codex,ChatGPT -ErrorAction SilentlyContinue | ForEach-Object {
  $windowTitles[[uint32]$_.Id] = $_.MainWindowTitle
}

Get-CimInstance Win32_Process |
  Where-Object {
    $_.Name -ieq 'Codex.exe' -or
    $_.Name -ieq 'codex.exe' -or
    $_.Name -ieq 'ChatGPT.exe'
  } |
  ForEach-Object {
    [PSCustomObject]@{
      Name = $_.Name
      ProcessId = [uint32]$_.ProcessId
      ParentProcessId = [uint32]$_.ParentProcessId
      CommandLine = if ($_.CommandLine) { $_.CommandLine } else { '' }
      ExecutablePath = if ($_.ExecutablePath) { $_.ExecutablePath } else { '' }
      MainWindowTitle = if ($windowTitles.ContainsKey([uint32]$_.ProcessId)) {
        [string]$windowTitles[[uint32]$_.ProcessId]
      } else {
        ''
      }
    }
  } |
  ConvertTo-Json -Compress
"#;

    let output = Command::new("powershell.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            POWERSHELL_SCRIPT,
        ])
        .output()
        .context("failed to query Windows process list")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("PowerShell process query failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let processes = parse_windows_codex_processes(&stdout)?;

    let mut active_processes = Vec::new();
    let mut ignored_count = 0;

    for process in processes
        .iter()
        .filter(|process| is_windows_codex_root_process(process))
    {
        let command = process.command_line.to_ascii_lowercase();
        if is_ide_plugin_process(&command) {
            ignored_count += 1;
            continue;
        }

        let has_window = !process.main_window_title.trim().is_empty();
        let has_renderer =
            windows_has_descendant_matching(process.process_id, &processes, |child| {
                child
                    .command_line
                    .to_ascii_lowercase()
                    .contains("--type=renderer")
            });
        let has_app_server =
            windows_has_descendant_matching(process.process_id, &processes, |child| {
                let command = child.command_line.to_ascii_lowercase();
                command.contains("resources\\codex.exe") && command.contains("app-server")
            });

        if has_window || has_renderer || has_app_server {
            active_processes.push(ActiveCodexProcess {
                pid: process.process_id,
                command_line: process.command_line.clone(),
                executable_path: non_empty_string(process.executable_path.clone()),
            });
        } else {
            // Ignore stale helper trees left behind after the window has already closed.
            ignored_count += 1;
        }
    }

    active_processes.sort_by_key(|process| process.pid);
    active_processes.dedup_by_key(|process| process.pid);

    Ok((active_processes, ignored_count))
}

#[cfg(windows)]
fn parse_windows_codex_processes(stdout: &str) -> anyhow::Result<Vec<WindowsCodexProcess>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let value: serde_json::Value =
        serde_json::from_str(trimmed).context("failed to parse Windows process JSON")?;

    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(|value| {
                serde_json::from_value(value)
                    .context("failed to deserialize Windows Codex process entry")
            })
            .collect(),
        value => Ok(vec![serde_json::from_value(value)
            .context("failed to deserialize Windows Codex process entry")?]),
    }
}

#[cfg(windows)]
fn is_windows_codex_root_process(process: &WindowsCodexProcess) -> bool {
    let name = process.name.to_ascii_lowercase();
    let command = process.command_line.to_ascii_lowercase();
    let executable = process
        .executable_path
        .replace('\\', "/")
        .to_ascii_lowercase();

    // The Microsoft Store Codex package kept its OpenAI.Codex package identity but renamed
    // the Electron host from Codex.exe to ChatGPT.exe in the 26.707 release. Restrict the
    // ChatGPT name to that package path so a separately installed ChatGPT app is never stopped.
    let is_codex_desktop_host = name == "codex.exe"
        || (name == "chatgpt.exe" && executable.contains("/windowsapps/openai.codex_"));

    is_codex_desktop_host
        && !command.contains("codex-switcher")
        && !command.contains("--type=")
        && !command.contains("resources\\codex.exe")
}

#[cfg(any(unix, windows))]
fn is_ide_plugin_process(command: &str) -> bool {
    command.contains(".antigravity")
        || command.contains("openai.chatgpt")
        || command.contains(".vscode")
}

#[cfg(windows)]
fn windows_has_descendant_matching<F>(
    root_pid: u32,
    processes: &[WindowsCodexProcess],
    mut predicate: F,
) -> bool
where
    F: FnMut(&WindowsCodexProcess) -> bool,
{
    let mut queue = vec![root_pid];
    let mut visited = HashSet::new();

    while let Some(parent_pid) = queue.pop() {
        for process in processes
            .iter()
            .filter(|process| process.parent_process_id == parent_pid)
        {
            if !visited.insert(process.process_id) {
                continue;
            }

            if predicate(process) {
                return true;
            }

            queue.push(process.process_id);
        }
    }

    false
}

fn build_launch_spec(process: ActiveCodexProcess) -> anyhow::Result<CodexLaunchSpec> {
    let mut args = split_command_line(&process.command_line);
    let executable_path = process.executable_path.and_then(non_empty_string);

    let executable = if let Some(executable_path) = executable_path {
        if !args.is_empty() {
            args.remove(0);
        }
        executable_path
    } else if !args.is_empty() {
        args.remove(0)
    } else {
        anyhow::bail!(
            "Could not determine how to restart Codex process {}",
            process.pid
        );
    };

    if !is_codex_executable(&executable) {
        anyhow::bail!(
            "Refusing to restart unexpected executable for Codex process {}: {}",
            process.pid,
            executable
        );
    }

    Ok(CodexLaunchSpec {
        pid: process.pid,
        executable,
        args,
    })
}

fn is_codex_executable(executable: &str) -> bool {
    let normalized = executable
        .trim_matches('"')
        .replace('\\', "/")
        .to_ascii_lowercase();
    let file_name = Path::new(executable)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(executable)
        .trim_matches('"')
        .to_ascii_lowercase();

    file_name == "codex"
        || file_name == "codex.exe"
        || (file_name == "chatgpt.exe" && normalized.contains("/windowsapps/openai.codex_"))
}

fn split_command_line(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut saw_arg = false;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                saw_arg = true;
            }
            '\\' if in_quotes && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
                saw_arg = true;
            }
            ch if ch.is_whitespace() && !in_quotes => {
                if saw_arg {
                    args.push(std::mem::take(&mut current));
                    saw_arg = false;
                }
            }
            ch => {
                current.push(ch);
                saw_arg = true;
            }
        }
    }

    if saw_arg {
        args.push(current);
    }

    args
}

fn non_empty_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn terminate_codex_process(pid: u32) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let output = Command::new("taskkill")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output()
            .with_context(|| format!("Failed to stop Codex process {pid}"))?;

        if !output.status.success() && codex_process_is_still_active(pid)? {
            anyhow::bail!(
                "Failed to stop Codex process {pid}: {}",
                command_failure_details(&output)
            );
        }
    }

    #[cfg(unix)]
    {
        let output = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output()
            .with_context(|| format!("Failed to stop Codex process {pid}"))?;

        if !output.status.success() && codex_process_is_still_active(pid)? {
            anyhow::bail!(
                "Failed to stop Codex process {pid}: {}",
                command_failure_details(&output)
            );
        }
    }

    Ok(())
}

fn terminate_antigravity_process(pid: u32) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        let output = Command::new("taskkill")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/F", "/PID", &pid.to_string()])
            .output()
            .with_context(|| format!("Failed to stop Antigravity process {pid}"))?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to stop Antigravity process {pid}: {}",
                command_failure_details(&output)
            );
        }
    }

    #[cfg(unix)]
    {
        let output = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output()
            .with_context(|| format!("Failed to stop Antigravity process {pid}"))?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to stop Antigravity process {pid}: {}",
                command_failure_details(&output)
            );
        }
    }

    Ok(())
}

fn codex_process_is_still_active(pid: u32) -> anyhow::Result<bool> {
    let (active_pids, _) = find_codex_processes()?;
    Ok(active_pids.contains(&pid))
}

fn wait_until_switch_guard_clears() -> anyhow::Result<()> {
    let deadline = Instant::now() + CODEX_STOP_TIMEOUT;
    loop {
        let (active_pids, _) = find_codex_processes()?;

        if active_pids.is_empty() {
            return Ok(());
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "Timed out waiting for Codex to close. Remaining processes: {:?}",
                active_pids
            );
        }

        thread::sleep(CODEX_STOP_POLL_INTERVAL);
    }
}

fn launch_codex_process(process: &CodexLaunchSpec) -> anyhow::Result<()> {
    let mut command = Command::new(&process.executable);
    command.args(&process.args);

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    command
        .spawn()
        .with_context(|| format!("Failed to restart Codex from {}", process.executable))?;

    Ok(())
}

fn command_failure_details(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }

    format!("process exited with status {}", output.status)
}

/// Find all running Antigravity codex assistant processes.
fn find_antigravity_processes() -> anyhow::Result<Vec<u32>> {
    let mut pids = Vec::new();

    #[cfg(unix)]
    {
        let output = Command::new("ps").args(["-eo", "pid,command"]).output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines().skip(1) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some((pid_str, command)) = line.split_once(' ') {
                let pid_str = pid_str.trim();
                let command = command.trim();

                if is_antigravity_app_server_command(command) {
                    if let Ok(pid) = pid_str.parse::<u32>() {
                        pids.push(pid);
                    }
                }
            }
        }
    }

    #[cfg(windows)]
    {
        const POWERSHELL_SCRIPT: &str = r#"
Get-CimInstance Win32_Process |
  Where-Object { $_.Name -ieq 'Codex.exe' -or $_.Name -ieq 'codex.exe' } |
  ForEach-Object {
    [PSCustomObject]@{
      ProcessId = [uint32]$_.ProcessId
      CommandLine = if ($_.CommandLine) { [string]$_.CommandLine } else { '' }
    }
  } |
  ConvertTo-Json -Compress
"#;

        let output = Command::new("powershell.exe")
            .creation_flags(CREATE_NO_WINDOW)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                POWERSHELL_SCRIPT,
            ])
            .output()?;

        if !output.status.success() {
            anyhow::bail!(
                "PowerShell Antigravity process query failed: {}",
                command_failure_details(&output)
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for process in parse_windows_codex_processes(&stdout)? {
            if is_antigravity_app_server_command(&process.command_line) {
                pids.push(process.process_id);
            }
        }
    }

    Ok(pids)
}

#[cfg(any(unix, windows))]
fn is_antigravity_app_server_command(command: &str) -> bool {
    let normalized = command.replace('\\', "/").to_ascii_lowercase();
    let is_ide_extension = normalized.contains(".antigravity/extensions/openai.chatgpt")
        || normalized.contains(".vscode/extensions/openai.chatgpt");

    is_ide_extension && normalized.contains("app-server")
}

#[cfg(test)]
mod tests {
    use super::{
        build_launch_spec, ensure_switch_allowed, is_antigravity_app_server_command,
        is_codex_executable, split_command_line, ActiveCodexProcess,
    };

    #[test]
    fn switch_guard_blocks_when_test_override_reports_live_processes() {
        let _guard = crate::test_support::env_lock();
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "2");
        let result = ensure_switch_allowed();
        std::env::remove_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT");
        assert!(result.is_err());
    }

    #[test]
    fn switch_guard_allows_when_test_override_reports_no_processes() {
        let _guard = crate::test_support::env_lock();
        std::env::set_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT", "0");
        let result = ensure_switch_allowed();
        std::env::remove_var("CODEX_SWITCHER_TEST_ACTIVE_CODEX_COUNT");
        assert!(result.is_ok());
    }

    #[test]
    fn command_line_splitter_preserves_quoted_paths_and_args() {
        let args = split_command_line(
            r#""C:\Users\Dev\AppData\Local\Programs\Codex\Codex.exe" --profile "Work Account""#,
        );

        assert_eq!(
            args,
            vec![
                r#"C:\Users\Dev\AppData\Local\Programs\Codex\Codex.exe"#,
                "--profile",
                "Work Account",
            ]
        );
    }

    #[test]
    fn launch_spec_rejects_unexpected_executable() {
        let result = build_launch_spec(ActiveCodexProcess {
            pid: 42,
            command_line: r#""C:\Tools\not-codex.exe" --profile Work"#.to_string(),
            executable_path: Some(r#"C:\Tools\not-codex.exe"#.to_string()),
        });

        assert!(result
            .expect_err("non-Codex executable should be rejected")
            .to_string()
            .contains("Refusing to restart unexpected executable"));
    }

    #[test]
    fn codex_executable_detection_accepts_cli_and_desktop_names() {
        assert!(is_codex_executable("codex"));
        assert!(is_codex_executable(
            r#"C:\Users\Dev\AppData\Local\Programs\Codex\Codex.exe"#
        ));
        assert!(is_codex_executable(
            r#"C:\Program Files\WindowsApps\OpenAI.Codex_26.707.3748.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe"#
        ));
        assert!(!is_codex_executable(
            r#"C:\Users\Dev\AppData\Local\Programs\ChatGPT\ChatGPT.exe"#
        ));
        assert!(!is_codex_executable("not-codex.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_codex_root_detection_accepts_renamed_store_host_only() {
        let renamed_codex = super::WindowsCodexProcess {
            name: "ChatGPT.exe".to_string(),
            process_id: 100,
            parent_process_id: 1,
            command_line: r#""C:\Program Files\WindowsApps\OpenAI.Codex_26.707.3748.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe""#.to_string(),
            executable_path: r#"C:\Program Files\WindowsApps\OpenAI.Codex_26.707.3748.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe"#.to_string(),
            main_window_title: "Codex".to_string(),
        };
        let standalone_chatgpt = super::WindowsCodexProcess {
            executable_path: r#"C:\Users\Dev\AppData\Local\Programs\ChatGPT\ChatGPT.exe"#
                .to_string(),
            ..renamed_codex.clone()
        };

        assert!(super::is_windows_codex_root_process(&renamed_codex));
        assert!(!super::is_windows_codex_root_process(&standalone_chatgpt));
    }

    #[cfg(windows)]
    #[test]
    fn launch_spec_accepts_renamed_store_host() {
        let path = r#"C:\Program Files\WindowsApps\OpenAI.Codex_26.707.3748.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe"#;
        let spec = build_launch_spec(ActiveCodexProcess {
            pid: 42,
            command_line: format!(r#""{path}""#),
            executable_path: Some(path.to_string()),
        })
        .expect("renamed Codex desktop host should be restartable");

        assert_eq!(spec.executable, path);
        assert!(spec.args.is_empty());
    }

    #[test]
    fn antigravity_restart_matcher_ignores_plain_codex_cli() {
        assert!(!is_antigravity_app_server_command(
            r#"C:\Users\Dev\AppData\Local\Programs\Codex\codex.exe exec "hello""#
        ));
        assert!(!is_antigravity_app_server_command(
            r#"/usr/local/bin/codex app-server"#
        ));
    }

    #[test]
    fn antigravity_restart_matcher_detects_ide_app_server() {
        assert!(is_antigravity_app_server_command(
            r#"C:\Users\Dev\.vscode\extensions\openai.chatgpt\bin\codex.exe" app-server --analytics-default-enabled"#
        ));
        assert!(is_antigravity_app_server_command(
            "/Users/dev/.antigravity/extensions/openai.chatgpt/bin/codex app-server"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn antigravity_windows_json_payload_detects_app_server_pid() {
        let stdout = r#"{
            "ProcessId": 1234,
            "CommandLine": "C:\\Users\\Dev\\.vscode\\extensions\\openai.chatgpt\\bin\\codex.exe app-server --analytics-default-enabled"
        }"#;

        let processes = super::parse_windows_codex_processes(stdout).expect("parse process JSON");
        let matching_pids = processes
            .into_iter()
            .filter(|process| is_antigravity_app_server_command(&process.command_line))
            .map(|process| process.process_id)
            .collect::<Vec<_>>();

        assert_eq!(matching_pids, vec![1234]);
    }
}

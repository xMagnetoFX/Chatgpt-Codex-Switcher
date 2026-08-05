import type { ReactNode } from "react";
import type { CodexProcessInfo } from "../types";
import type { ThemeMode } from "../types/ui";

interface SettingsViewProps {
  allMasked: boolean;
  autoWarmupEnabled: boolean;
  restartSwitchEnabled: boolean;
  themeMode: ThemeMode;
  isRefreshing: boolean;
  isAutoWarmupRunning: boolean;
  isExportingSlim: boolean;
  isImportingSlim: boolean;
  isExportingFull: boolean;
  isImportingFull: boolean;
  processInfo: CodexProcessInfo | null;
  hasRunningProcesses: boolean;
  onImportFullFile: () => void;
  onExportSlimText: () => void;
  onImportSlimText: () => void;
  onExportFullFile: () => void;
  onToggleMaskAll: () => void;
  onToggleAutoWarmup: () => void;
  onToggleRestartSwitch: () => void;
  onToggleTheme: () => void;
}

export function SettingsView({
  allMasked,
  autoWarmupEnabled,
  restartSwitchEnabled,
  themeMode,
  isRefreshing,
  isAutoWarmupRunning,
  isExportingSlim,
  isImportingSlim,
  isExportingFull,
  isImportingFull,
  processInfo,
  hasRunningProcesses,
  onImportFullFile,
  onExportSlimText,
  onImportSlimText,
  onExportFullFile,
  onToggleMaskAll,
  onToggleAutoWarmup,
  onToggleRestartSwitch,
  onToggleTheme,
}: SettingsViewProps) {
  return (
    <div className="settings-native-page">
      <SettingsGroup
        title="Workspace"
        description="Privacy, appearance, automation, and account switching behavior."
      >
        <PreferenceRow
          title="Account identities"
          description={
            allMasked
              ? "Names, emails, and initials are hidden across the app."
              : "Names, emails, and initials are visible across the app."
          }
          control={
            <NativeSwitch
              active={allMasked}
              label={allMasked ? "Reveal all accounts" : "Hide all accounts"}
              onClick={onToggleMaskAll}
            />
          }
        />
        <PreferenceRow
          title="Appearance"
          description="Choose the color mode used throughout the Switcher."
          control={
            <div className="settings-segmented-control" role="group" aria-label="Appearance">
              <button
                type="button"
                aria-label="Switch to light mode"
                aria-pressed={themeMode === "light"}
                className={themeMode === "light" ? "selected" : ""}
                onClick={() => {
                  if (themeMode !== "light") onToggleTheme();
                }}
              >
                Light
              </button>
              <button
                type="button"
                aria-label="Switch to dark mode"
                aria-pressed={themeMode === "dark"}
                className={themeMode === "dark" ? "selected" : ""}
                onClick={() => {
                  if (themeMode !== "dark") onToggleTheme();
                }}
              >
                Dark
              </button>
            </div>
          }
        />
        <PreferenceRow
          title="Automatic warm-up"
          description={
            isAutoWarmupRunning
              ? "A warm-up cycle is running now."
              : autoWarmupEnabled
                ? "Runs after launch and hourly while the app remains open."
                : "No background warm-up traffic will be sent."
          }
          control={
            <NativeSwitch
              active={autoWarmupEnabled}
              busy={isAutoWarmupRunning}
              label={autoWarmupEnabled ? "Auto warm-up enabled" : "Auto warm-up disabled"}
              onClick={onToggleAutoWarmup}
            />
          }
        />
        <PreferenceRow
          title="Restart Codex when switching"
          description={
            restartSwitchEnabled
              ? "Close and reopen Codex automatically when a safe account switch requires it."
              : "Account switching remains locked until Codex is closed."
          }
          control={
            <NativeSwitch
              active={restartSwitchEnabled}
              label={
                restartSwitchEnabled ? "Restart switching enabled" : "Restart switching disabled"
              }
              onClick={onToggleRestartSwitch}
            />
          }
        />
      </SettingsGroup>

      <SettingsGroup
        title="Account transfer"
        description="Move missing accounts or keep an encrypted recovery copy of the complete store."
      >
        <TransferRow
          title="Slim text transfer"
          description="Copy or merge a compact clipboard payload containing missing accounts."
          actions={
            <>
              <SettingsButton
                label={isImportingSlim ? "Importing slim text…" : "Import slim text"}
                onClick={onImportSlimText}
                disabled={isImportingSlim}
              />
              <SettingsButton
                label={isExportingSlim ? "Exporting slim text…" : "Export slim text"}
                onClick={onExportSlimText}
                disabled={isExportingSlim}
              />
            </>
          }
        />
        <TransferRow
          title="Encrypted backup"
          description="Restore or save every stored account using the protected `.cswf` desktop format."
          actions={
            <>
              <SettingsButton
                label={isImportingFull ? "Restoring full backup…" : "Restore full backup"}
                onClick={onImportFullFile}
                disabled={isImportingFull}
              />
              <SettingsButton
                label={isExportingFull ? "Exporting full backup…" : "Export full backup"}
                onClick={onExportFullFile}
                disabled={isExportingFull}
                primary
              />
            </>
          }
        />
      </SettingsGroup>

      <ProcessSafetyStatus
        processInfo={processInfo}
        hasRunningProcesses={hasRunningProcesses}
        restartSwitchEnabled={restartSwitchEnabled}
        isRefreshing={isRefreshing}
      />
    </div>
  );
}

function SettingsGroup({
  title,
  description,
  children,
}: {
  title: string;
  description: string;
  children: ReactNode;
}) {
  const headingId = `settings-${title.toLowerCase().replace(/\s+/g, "-")}`;

  return (
    <section className="settings-native-group" aria-labelledby={headingId}>
      <div className="settings-native-group-heading">
        <h3 id={headingId}>{title}</h3>
        <p>{description}</p>
      </div>
      <div className="settings-native-list">{children}</div>
    </section>
  );
}

function PreferenceRow({
  title,
  description,
  control,
}: {
  title: string;
  description: string;
  control: ReactNode;
}) {
  return (
    <div className="settings-native-row">
      <div className="settings-native-row-copy">
        <strong>{title}</strong>
        <span>{description}</span>
      </div>
      <div className="settings-native-row-control">{control}</div>
    </div>
  );
}

function TransferRow({
  title,
  description,
  actions,
}: {
  title: string;
  description: string;
  actions: ReactNode;
}) {
  return (
    <div className="settings-native-row settings-native-transfer-row">
      <div className="settings-native-row-copy">
        <strong>{title}</strong>
        <span>{description}</span>
      </div>
      <div className="settings-native-actions">{actions}</div>
    </div>
  );
}

function NativeSwitch({
  active,
  busy = false,
  label,
  onClick,
}: {
  active: boolean;
  busy?: boolean;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={`settings-native-switch ${active ? "on" : ""} ${busy ? "busy" : ""}`}
      aria-label={label}
      aria-pressed={active}
      onClick={onClick}
    >
      <span />
    </button>
  );
}

function SettingsButton({
  label,
  onClick,
  disabled,
  primary = false,
}: {
  label: string;
  onClick: () => void;
  disabled: boolean;
  primary?: boolean;
}) {
  return (
    <button
      type="button"
      className={`settings-native-button ${primary ? "primary" : ""}`}
      onClick={onClick}
      disabled={disabled}
    >
      {label}
    </button>
  );
}

function ProcessSafetyStatus({
  processInfo,
  hasRunningProcesses,
  restartSwitchEnabled,
  isRefreshing,
}: {
  processInfo: CodexProcessInfo | null;
  hasRunningProcesses: boolean;
  restartSwitchEnabled: boolean;
  isRefreshing: boolean;
}) {
  const message = processInfo
    ? hasRunningProcesses
      ? restartSwitchEnabled
        ? `Switching will restart ${processInfo.count} Codex ${processInfo.count === 1 ? "process" : "processes"}.`
        : `Switching is locked until ${processInfo.count} Codex ${processInfo.count === 1 ? "process closes" : "processes close"}.`
      : isRefreshing
        ? "No Codex processes are active, and refreshes are safe to run right now."
        : "No Codex processes are active. Switching and refreshes are allowed."
    : "Process status is still loading.";

  return (
    <aside className={`settings-native-process ${hasRunningProcesses ? "busy" : ""}`}>
      <div>
        <strong>Process safety</strong>
        <span>{message}</span>
      </div>
      <div className="settings-native-process-state">
        <i />
        {processInfo ? `${processInfo.background_count} background` : "Checking"}
      </div>
    </aside>
  );
}

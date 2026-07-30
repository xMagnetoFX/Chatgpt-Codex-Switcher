import type { AccountWithUsage, CodexProcessInfo } from "../types";
import type { SortMode } from "../types/ui";
import {
  AccountsIcon,
  ChevronDownIcon,
  EyeIcon,
  EyeOffIcon,
  PersonIcon,
  RefreshIcon,
  SwitchIcon,
  WarningIcon,
} from "./icons";
import { AccountCard } from "./AccountCard";
import { StatePanel } from "./ui";

interface HomeDashboardProps {
  title: string;
  heading: string;
  description: string;
  accounts: AccountWithUsage[];
  activeAccount?: AccountWithUsage;
  otherAccounts: AccountWithUsage[];
  sortedOtherAccounts: AccountWithUsage[];
  loading: boolean;
  error: string | null;
  processInfo: CodexProcessInfo | null;
  hasRunningProcesses: boolean;
  activeAccountMasked: boolean;
  attentionCount: number;
  switchingId: string | null;
  isRefreshing: boolean;
  isWindowMaximized: boolean;
  allMasked: boolean;
  restartSwitchEnabled: boolean;
  maskedAccounts: Set<string>;
  otherAccountsSort: SortMode;
  onSortChange: (sort: SortMode) => void;
  onRefresh: () => void;
  onReloadWorkspace: () => void;
  onSwitch: (accountId: string) => void;
  onDelete: (accountId: string) => void;
  onRename: (accountId: string, newName: string) => Promise<void>;
  onToggleMaskAll: () => void;
}

export function HomeDashboard({
  title,
  heading,
  description,
  accounts,
  activeAccount,
  otherAccounts,
  sortedOtherAccounts,
  loading,
  error,
  processInfo,
  hasRunningProcesses,
  activeAccountMasked,
  attentionCount,
  switchingId,
  isRefreshing,
  isWindowMaximized,
  allMasked,
  restartSwitchEnabled,
  maskedAccounts,
  otherAccountsSort,
  onSortChange,
  onRefresh,
  onReloadWorkspace,
  onSwitch,
  onDelete,
  onRename,
  onToggleMaskAll,
}: HomeDashboardProps) {
  if (loading && accounts.length === 0) {
    return (
      <StatePanel
        eyebrow="Loading"
        title="Pulling account state"
        description="We're loading saved accounts and refreshing their usage windows."
        loading
      />
    );
  }

  if (error) {
    return (
      <StatePanel
        eyebrow="Load error"
        title="Failed to load accounts"
        description={error}
        tone="danger"
        action={
          <button type="button" onClick={onReloadWorkspace} className="btn-ghost">
            <RefreshIcon className="h-4 w-4" />
            Refresh again
          </button>
        }
      />
    );
  }

  if (accounts.length === 0) {
    return (
      <StatePanel
        eyebrow="Fresh workspace"
        title="No accounts yet"
        description="Use the Add account button in the left panel to start tracking limits and switching cleanly."
      />
    );
  }

  return (
    <div className="flex flex-col gap-5">
      {/* Page header + metrics — wrapped in a named region for tests */}
      <section aria-label="Home summary" className="flex flex-col gap-5">
      <div className="page-header">
        <div>
          <div className="kicker">{title}</div>
          <h2
            style={{
              margin: 0,
              fontSize: 26,
              fontWeight: 600,
              letterSpacing: "-0.01em",
              color: "var(--text-strong)",
            }}
          >
            {heading}
          </h2>
          <p style={{ margin: "6px 0 0", fontSize: "13.5px", color: "var(--text-muted)", maxWidth: 540, lineHeight: 1.55 }}>
            {description}
          </p>
        </div>
        <div className="header-actions">
          <button
            type="button"
            onClick={onToggleMaskAll}
            disabled={accounts.length === 0}
            title={allMasked ? "Reveal all identities" : "Hide all identities"}
            aria-label={allMasked ? "Reveal all identities" : "Hide all identities"}
            className="icon-btn-lg"
          >
            {allMasked ? <EyeIcon className="h-4 w-4" /> : <EyeOffIcon className="h-4 w-4" />}
            <span className="hidden lg:inline">{allMasked ? "Reveal" : "Hide"}</span>
          </button>
          <button
            type="button"
            onClick={onRefresh}
            disabled={isRefreshing}
            title="Refresh account usage"
            aria-label="Refresh account usage"
            className="icon-btn-lg"
          >
            <RefreshIcon className={`h-4 w-4 ${isRefreshing ? "animate-spin" : ""}`} />
            <span className="hidden lg:inline">{isRefreshing ? "Refreshing" : "Refresh"}</span>
          </button>
        </div>
      </div>

      {/* Metric tiles */}
      <div className="metrics">
        <div className="metric">
          <div className="metric-head">
            <div className="metric-label">Total accounts</div>
            <span className="metric-icon" aria-hidden="true">
              <AccountsIcon />
            </span>
          </div>
          <div className="metric-value">{accounts.length}</div>
          <div className="metric-note">stored in your local switcher workspace</div>
        </div>
        <div className="metric metric-active">
          <div className="metric-head">
            <div className="metric-label">Active account</div>
            <span className="metric-icon" aria-hidden="true">
              <PersonIcon />
            </span>
          </div>
          <div className="metric-value">
            {activeAccount ? (activeAccountMasked ? "Hidden" : activeAccount.name) : "None"}
          </div>
          <div className="metric-note">
            {activeAccountMasked
              ? "identity hidden"
              : activeAccount?.email
                ? activeAccount.email
                : activeAccount
                  ? "ready but email hidden"
                  : "select an account to activate"}
          </div>
        </div>
        <div className={`metric ${hasRunningProcesses ? "metric-caution" : "metric-ready"}`}>
          <div className="metric-head">
            <div className="metric-label">Switch status</div>
            <span className="metric-icon" aria-hidden="true">
              <SwitchIcon />
            </span>
          </div>
          <div className="metric-value">
            {processInfo
              ? hasRunningProcesses
                ? restartSwitchEnabled ? "Restart" : "Locked"
                : "Ready"
              : "Checking"}
          </div>
          <div className="metric-note">
            {processInfo
              ? hasRunningProcesses
                ? restartSwitchEnabled
                  ? "switching will close and reopen Codex"
                  : "enable restart switching or close Codex"
                : "safe to activate another account"
              : "querying live process state"}
          </div>
        </div>
        <div className={`metric ${attentionCount > 0 ? "metric-caution" : "metric-clear"}`}>
          <div className="metric-head">
            <div className="metric-label">Attention</div>
            <span className="metric-icon" aria-hidden="true">
              <WarningIcon />
            </span>
          </div>
          <div className="metric-value">{attentionCount > 0 ? attentionCount : "Clear"}</div>
          <div className="metric-note">
            {attentionCount > 0
              ? "accounts with low remaining limits or usage errors"
              : "no low-limit or error accounts detected"}
          </div>
        </div>
      </div>
      </section>

      {/* Featured active account */}
      {activeAccount && (
        <AccountCard
          account={activeAccount}
          onSwitch={() => {}}
          onDelete={() => onDelete(activeAccount.id)}
          onRename={(newName) => onRename(activeAccount.id, newName)}
          switching={switchingId === activeAccount.id}
          switchInProgress={switchingId !== null}
          switchDisabled={hasRunningProcesses}
          restartSwitchEnabled={restartSwitchEnabled}
          masked={maskedAccounts.has(activeAccount.id)}
          featured
        />
      )}

      {/* Standby section */}
      <div className="flex flex-col gap-4">
        <div className="section-head">
          <h3>Other accounts ({otherAccounts.length})</h3>
          {otherAccounts.length > 0 && (
            <div className="relative inline-flex items-center">
              <select
                value={otherAccountsSort}
                onChange={(event) => onSortChange(event.target.value as SortMode)}
                className="select-glass"
              >
                <option value="deadline_asc">Reset: earliest to latest</option>
                <option value="deadline_desc">Reset: latest to earliest</option>
                <option value="remaining_desc">% remaining: highest to lowest</option>
                <option value="remaining_asc">% remaining: lowest to highest</option>
              </select>
              <ChevronDownIcon className="pointer-events-none absolute right-3 h-3.5 w-3.5 text-[color:var(--text-faint)]" />
            </div>
          )}
        </div>

        {otherAccounts.length > 0 ? (
          <div
            className={`standby-account-grid ${
              isWindowMaximized ? "standby-account-grid-maximized" : ""
            }`}
          >
            {sortedOtherAccounts.map((account) => (
              <AccountCard
                key={account.id}
                account={account}
                onSwitch={() => onSwitch(account.id)}
                onDelete={() => onDelete(account.id)}
                onRename={(newName) => onRename(account.id, newName)}
                switching={switchingId === account.id}
                switchInProgress={switchingId !== null}
                switchDisabled={hasRunningProcesses}
                restartSwitchEnabled={restartSwitchEnabled}
                masked={maskedAccounts.has(account.id)}
              />
            ))}
          </div>
        ) : (
          <div className="glass-surface rounded-[20px] p-5 text-sm leading-6 text-[color:var(--text-muted)]">
            There are no standby accounts yet. Add another account from the sidebar or switch to
            Settings to import/export profiles.
          </div>
        )}
      </div>
    </div>
  );
}

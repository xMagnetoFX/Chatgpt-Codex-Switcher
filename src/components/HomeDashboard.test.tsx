import { render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import type { AccountWithUsage, CodexProcessInfo } from "../types";
import type { SortMode } from "../types/ui";
import { HomeDashboard } from "./HomeDashboard";

const processReady: CodexProcessInfo = {
  count: 0,
  background_count: 0,
  can_switch: true,
  pids: [],
};

const processLocked: CodexProcessInfo = {
  count: 1,
  background_count: 0,
  can_switch: false,
  pids: [1234],
};

function makeAccount(id: string, name: string, email: string, isActive = false): AccountWithUsage {
  return {
    id,
    name,
    email,
    plan_type: "plus",
    auth_mode: "chat_gpt",
    is_active: isActive,
    created_at: "2026-04-20T00:00:00Z",
    last_used_at: null,
    usageLoading: false,
    usage: {
      account_id: id,
      plan_type: "plus",
      primary_used_percent: 12,
      primary_window_minutes: 300,
      primary_resets_at: Math.floor(Date.now() / 1000) + 7200,
      secondary_used_percent: 30,
      secondary_window_minutes: 10080,
      secondary_resets_at: Math.floor(Date.now() / 1000) + 86400,
      has_credits: true,
      unlimited_credits: false,
      credits_balance: "0",
      banked_resets: 0,
      error: null,
    },
  };
}

const activeAccount = makeAccount("active", "Active Pilot", "active@example.com", true);
const standbyAccount = makeAccount("standby", "Standby Pilot", "standby@example.com");

function renderDashboard(overrides: Partial<ComponentProps<typeof HomeDashboard>> = {}) {
  const accounts = overrides.accounts ?? [activeAccount, standbyAccount];
  const otherAccounts = overrides.otherAccounts ?? accounts.filter((account) => !account.is_active);

  return render(
    <HomeDashboard
      title="Home"
      heading="Account cockpit"
      description="Scan account state, refresh usage, and switch only when Codex is quiet."
      accounts={accounts}
      activeAccount={overrides.activeAccount ?? accounts.find((account) => account.is_active)}
      otherAccounts={otherAccounts}
      sortedOtherAccounts={overrides.sortedOtherAccounts ?? otherAccounts}
      loading={false}
      error={null}
      processInfo={processReady}
      hasRunningProcesses={false}
      activeAccountMasked={false}
      attentionCount={0}
      switchingId={null}
      isRefreshing={false}
      isWindowMaximized={false}
      allMasked={false}
      restartSwitchEnabled={true}
      maskedAccounts={new Set()}
      otherAccountsSort={"deadline_asc" as SortMode}
      onSortChange={vi.fn()}
      onRefresh={vi.fn()}
      onReloadWorkspace={vi.fn()}
      onSwitch={vi.fn()}
      onDelete={vi.fn()}
      onRename={vi.fn().mockResolvedValue(undefined)}
      onToggleMaskAll={vi.fn()}
      {...overrides}
    />
  );
}

describe("HomeDashboard", () => {
  it("renders the integrated summary shell with refresh and metric cards", () => {
    renderDashboard();

    const summary = screen.getByRole("region", { name: /Home summary/i });

    expect(summary).toHaveTextContent("Home");
    expect(summary).toHaveTextContent("Account cockpit");
    expect(summary).toHaveTextContent("Total accounts");
    expect(summary).toHaveTextContent("Active account");
    expect(summary).toHaveTextContent("Switch status");
    expect(summary).toHaveTextContent("Attention");
    expect(screen.getByRole("button", { name: /Hide all identities/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Refresh account usage/i })).toBeInTheDocument();
  });

  it("renders active and standby account sections", () => {
    renderDashboard();

    expect(screen.queryByText("Live profile")).not.toBeInTheDocument();
    expect(screen.getByText("Other accounts (1)")).toBeInTheDocument();
    expect(screen.getAllByText("Active Pilot").length).toBeGreaterThan(0);
    expect(screen.getByText("Standby Pilot")).toBeInTheDocument();
  });

  it("keeps dashboard summaries and cards private when an account is masked", () => {
    renderDashboard({
      activeAccountMasked: true,
      maskedAccounts: new Set(["active"]),
    });

    expect(screen.queryByText("Active Pilot")).not.toBeInTheDocument();
    expect(screen.queryByText("active@example.com")).not.toBeInTheDocument();
    expect(screen.getByText("Hidden")).toBeInTheDocument();
    expect(screen.getByText("identity hidden")).toBeInTheDocument();
    expect(screen.getByText("Hidden account")).toBeInTheDocument();
  });

  it("offers restart switching while Codex processes are running", () => {
    renderDashboard({ processInfo: processLocked, hasRunningProcesses: true });

    expect(screen.getByText("Restart")).toBeInTheDocument();
    expect(screen.getByText("switching will close and reopen Codex")).toBeInTheDocument();
    expect(screen.queryByText("Close Codex to switch accounts")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Restart & switch" })).toBeEnabled();
  });

  it("locks switch actions while Codex runs when restart switching is disabled", () => {
    renderDashboard({
      processInfo: processLocked,
      hasRunningProcesses: true,
      restartSwitchEnabled: false,
    });

    expect(screen.getByText("Locked")).toBeInTheDocument();
    expect(screen.getByText("enable restart switching or close Codex")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Codex running" })).toBeDisabled();
  });

  it("renders loading, error, and empty states", () => {
    const baseProps = {
      title: "Home",
      heading: "Account cockpit",
      description: "Scan account state, refresh usage, and switch only when Codex is quiet.",
      accounts: [],
      activeAccount: undefined,
      otherAccounts: [],
      sortedOtherAccounts: [],
      processInfo: processReady,
      hasRunningProcesses: false,
      activeAccountMasked: false,
      attentionCount: 0,
      switchingId: null,
      isRefreshing: false,
      isWindowMaximized: false,
      allMasked: false,
      restartSwitchEnabled: true,
      maskedAccounts: new Set<string>(),
      otherAccountsSort: "deadline_asc" as SortMode,
      onSortChange: vi.fn(),
      onRefresh: vi.fn(),
      onReloadWorkspace: vi.fn(),
      onSwitch: vi.fn(),
      onDelete: vi.fn(),
      onRename: vi.fn().mockResolvedValue(undefined),
      onToggleMaskAll: vi.fn(),
    };

    const { rerender } = render(<HomeDashboard {...baseProps} loading error={null} />);

    expect(screen.getByText("Pulling account state")).toBeInTheDocument();

    rerender(<HomeDashboard {...baseProps} loading={false} error="Kaboom" />);

    expect(screen.getByText("Failed to load accounts")).toBeInTheDocument();
    expect(screen.getByText("Kaboom")).toBeInTheDocument();

    rerender(<HomeDashboard {...baseProps} loading={false} error={null} />);

    expect(screen.getByText("No accounts yet")).toBeInTheDocument();
  });
});

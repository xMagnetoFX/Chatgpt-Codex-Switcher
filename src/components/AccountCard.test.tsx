import { render, screen } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import type { AccountWithUsage } from "../types";
import { AccountCard } from "./AccountCard";

const baseAccount: AccountWithUsage = {
  id: "acct-1",
  name: "AC3 Private",
  email: "private@example.com",
  plan_type: "plus",
  auth_mode: "chat_gpt",
  is_active: false,
  created_at: "2026-04-20T00:00:00Z",
  last_used_at: null,
  usageLoading: false,
  usage: {
    account_id: "acct-1",
    plan_type: "plus",
    primary_used_percent: 20,
    primary_window_minutes: 300,
    primary_resets_at: Math.floor(Date.now() / 1000) + 3600,
    secondary_used_percent: 10,
    secondary_window_minutes: 10080,
    secondary_resets_at: Math.floor(Date.now() / 1000) + 86400,
    has_credits: true,
    unlimited_credits: false,
    credits_balance: "0",
    error: null,
  },
};

function renderCard(overrides: Partial<ComponentProps<typeof AccountCard>> = {}) {
  return render(
    <AccountCard
      account={baseAccount}
      onSwitch={vi.fn()}
      onDelete={vi.fn()}
      onRename={vi.fn().mockResolvedValue(undefined)}
      {...overrides}
    />
  );
}

describe("AccountCard", () => {
  it("masks names and emails while keeping the user avatar decorative", () => {
    const { container } = renderCard({ masked: true });

    expect(screen.queryByText("AC3 Private")).not.toBeInTheDocument();
    expect(screen.queryByText("private@example.com")).not.toBeInTheDocument();
    expect(screen.getByText("Hidden account")).toBeInTheDocument();
    expect(screen.getByText("Hidden email")).toBeInTheDocument();
    expect(container.querySelector(".avatar-sm")).toHaveAttribute("aria-hidden", "true");
  });

  it("offers restart switching while Codex processes are running", () => {
    renderCard({ switchDisabled: true });

    expect(screen.getByRole("button", { name: "Restart & switch" })).toBeEnabled();
  });

  it("prefers the live usage plan over stale stored plan metadata", () => {
    renderCard({
      account: {
        ...baseAccount,
        plan_type: "plus",
        usage: { ...baseAccount.usage!, plan_type: "pro" },
      },
    });

    expect(screen.getByText("Pro")).toBeInTheDocument();
    expect(screen.queryByText("Plus")).not.toBeInTheDocument();
  });

  it("locks switching while Codex runs when restart switching is disabled", () => {
    renderCard({ switchDisabled: true, restartSwitchEnabled: false });

    expect(screen.getByRole("button", { name: "Codex running" })).toBeDisabled();
  });
});

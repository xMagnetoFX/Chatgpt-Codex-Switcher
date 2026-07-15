import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { UsageInfo } from "../types";
import { UsageBar } from "./UsageBar";

function usage(overrides: Partial<UsageInfo>): UsageInfo {
  return {
    account_id: "account-1",
    plan_type: "plus",
    primary_used_percent: null,
    primary_window_minutes: null,
    primary_resets_at: null,
    secondary_used_percent: null,
    secondary_window_minutes: null,
    secondary_resets_at: null,
    has_credits: null,
    unlimited_credits: null,
    credits_balance: null,
    banked_resets: null,
    error: null,
    ...overrides,
  };
}

describe("UsageBar", () => {
  it("labels a primary seven-day window as the weekly limit", () => {
    render(
      <UsageBar
        usage={usage({
          primary_used_percent: 42,
          primary_window_minutes: 7 * 24 * 60,
        })}
      />
    );

    expect(screen.getByText("Weekly limit (7d)")).toBeInTheDocument();
    expect(screen.queryByText("5h limit (7d)")).not.toBeInTheDocument();
  });

  it("keeps the older five-hour and weekly two-window payload correctly labelled", () => {
    render(
      <UsageBar
        usage={usage({
          primary_used_percent: 20,
          primary_window_minutes: 5 * 60,
          secondary_used_percent: 35,
          secondary_window_minutes: 7 * 24 * 60,
        })}
      />
    );

    expect(screen.getByText("5h limit (5h)")).toBeInTheDocument();
    expect(screen.getByText("Weekly limit (7d)")).toBeInTheDocument();
  });

  it("shows the number of banked resets, including zero", () => {
    render(
      <UsageBar
        usage={usage({
          primary_used_percent: 20,
          primary_window_minutes: 7 * 24 * 60,
          banked_resets: 0,
        })}
      />
    );

    expect(screen.getByText("Banked resets:")).toBeInTheDocument();
    expect(screen.getByText("0")).toBeInTheDocument();
  });
});

import { describe, expect, it } from "vitest";
import type { AccountWithUsage } from "../types";
import { formatPlanLabel, needsAttention } from "./accountDisplay";

function makeAccount(overrides: Partial<AccountWithUsage> = {}): AccountWithUsage {
  return {
    id: "acct-1",
    name: "API account",
    email: null,
    plan_type: "api_key",
    auth_mode: "api_key",
    is_active: false,
    created_at: "2026-04-20T00:00:00Z",
    last_used_at: null,
    usageLoading: false,
    usage: {
      account_id: "acct-1",
      plan_type: "api_key",
      primary_used_percent: null,
      primary_window_minutes: null,
      primary_resets_at: null,
      secondary_used_percent: null,
      secondary_window_minutes: null,
      secondary_resets_at: null,
      has_credits: null,
      unlimited_credits: null,
      credits_balance: null,
      error: null,
    },
    ...overrides,
  };
}

describe("accountDisplay", () => {
  it("uses an honest unknown label when ChatGPT returns no plan", () => {
    expect(formatPlanLabel({ plan_type: null, auth_mode: "chat_gpt" })).toBe("Unknown");
  });

  it("formats live multi-word plan identifiers", () => {
    expect(formatPlanLabel({ plan_type: "chatgpt_team", auth_mode: "chat_gpt" })).toBe(
      "ChatGPT Team"
    );
  });

  it("does not flag expected API-key no-usage state as needing attention", () => {
    expect(needsAttention(makeAccount())).toBe(false);
  });

  it("still flags real usage errors", () => {
    expect(
      needsAttention(
        makeAccount({
          usage: {
            account_id: "acct-1",
            plan_type: "api_key",
            primary_used_percent: null,
            primary_window_minutes: null,
            primary_resets_at: null,
            secondary_used_percent: null,
            secondary_window_minutes: null,
            secondary_resets_at: null,
            has_credits: null,
            unlimited_credits: null,
            credits_balance: null,
            error: "network unavailable",
          },
        })
      )
    ).toBe(true);
  });
});

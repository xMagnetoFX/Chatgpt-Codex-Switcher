import type { AccountWithUsage, UsageInfo } from "../types";

export function formatPlanLabel(
  account: Pick<AccountWithUsage, "plan_type" | "auth_mode">
): string {
  if (account.plan_type) {
    if (account.plan_type.toLowerCase() === "api_key") return "API Key";
    return account.plan_type
      .split(/[_-]+/)
      .filter(Boolean)
      .map((part) =>
        part.toLowerCase() === "chatgpt"
          ? "ChatGPT"
          : part.charAt(0).toUpperCase() + part.slice(1).toLowerCase()
      )
      .join(" ");
  }

  return account.auth_mode === "api_key" ? "API Key" : "Unknown";
}

export function getLowestRemaining(usage?: UsageInfo): number | null {
  if (!usage || usage.error) return null;

  const percentages = [usage.primary_used_percent, usage.secondary_used_percent]
    .filter((value): value is number => value !== null && value !== undefined)
    .map((usedPercent) => Math.max(0, 100 - usedPercent));

  if (percentages.length === 0) return null;
  return Math.min(...percentages);
}

export function needsAttention(account: AccountWithUsage): boolean {
  if (account.usage?.error) return true;
  const remaining = getLowestRemaining(account.usage);
  return remaining !== null && remaining <= 20;
}

export function getMaskedAccountLabel(masked: boolean, fallback = "account"): string {
  return masked ? "hidden account" : fallback;
}

export function getAccountInitials(name: string, masked: boolean): string {
  if (masked) return "**";
  const compact = name
    .split(/\s+/)
    .filter(Boolean)
    .map((part) => part[0])
    .join("")
    .slice(0, 2);

  return (compact || name.slice(0, 2) || "AC").toUpperCase();
}

export function getPrivatePlaceholder(kind: "name" | "email"): string {
  return kind === "name" ? "Hidden account" : "Hidden email";
}

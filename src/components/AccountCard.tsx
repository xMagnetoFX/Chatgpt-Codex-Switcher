import { useEffect, useRef, useState } from "react";
import type { AccountWithUsage } from "../types";
import { formatPlanLabel, getPrivatePlaceholder } from "../lib/accountDisplay";
import { PersonIcon, TrashIcon } from "./icons";
import { UsageBar } from "./UsageBar";

interface AccountCardProps {
  account: AccountWithUsage;
  onSwitch: () => void;
  onDelete: () => void;
  onRename: (newName: string) => Promise<void>;
  switching?: boolean;
  switchInProgress?: boolean;
  switchDisabled?: boolean;
  restartSwitchEnabled?: boolean;
  masked?: boolean;
  featured?: boolean;
}

function formatLastRefresh(date: Date | null): string {
  if (!date) return "Never";
  const now = new Date();
  const diff = Math.floor((now.getTime() - date.getTime()) / 1000);
  if (diff < 5) return "Just now";
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
  return date.toLocaleDateString();
}

function formatSubscriptionExpiry(timestamp: string | null): {
  label: string;
  isPast: boolean;
} | null {
  if (!timestamp) return null;

  const expiry = new Date(timestamp);
  if (Number.isNaN(expiry.getTime())) return null;

  return {
    label: `Active until ${new Intl.DateTimeFormat(undefined, {
      month: "short",
      day: "numeric",
      year: "numeric",
    }).format(expiry)}`,
    isPast: expiry.getTime() <= Date.now(),
  };
}

function PrivateText({
  children,
  masked,
  kind,
}: {
  children: string;
  masked: boolean;
  kind: "name" | "email";
}) {
  if (!masked) return <>{children}</>;
  return (
    <span aria-label={`${kind} hidden`} className="select-none tracking-[0.08em]">
      {getPrivatePlaceholder(kind)}
    </span>
  );
}

const planChipClass: Record<string, string> = {
  pro: "plan-chip plan-chip--pro",
  plus: "plan-chip plan-chip--plus",
  team: "plan-chip plan-chip--team",
  enterprise: "plan-chip plan-chip--enterprise",
  free: "plan-chip plan-chip--free",
  api_key: "plan-chip plan-chip--api",
  unknown: "plan-chip plan-chip--unknown",
};

export function AccountCard({
  account,
  onSwitch,
  onDelete,
  onRename,
  switching,
  switchInProgress,
  switchDisabled,
  restartSwitchEnabled = true,
  masked = false,
  featured = false,
}: AccountCardProps) {
  const [lastRefresh, setLastRefresh] = useState<Date | null>(
    account.usage && !account.usage.error ? new Date() : null
  );
  const [isEditing, setIsEditing] = useState(false);
  const [editName, setEditName] = useState(account.name);
  const inputRef = useRef<HTMLInputElement>(null);
  const isFeatured = featured || account.is_active;

  useEffect(() => {
    setEditName(account.name);
  }, [account.name]);

  useEffect(() => {
    setLastRefresh(null);
  }, [account.id]);

  useEffect(() => {
    if (account.usage && !account.usage.error) setLastRefresh(new Date());
  }, [account.usage]);

  // Re-render periodically so the relative "last refresh" label stays honest
  // between usage refreshes.
  const [, setRefreshTick] = useState(0);
  useEffect(() => {
    if (!lastRefresh) return;
    const interval = window.setInterval(() => setRefreshTick((tick) => tick + 1), 10000);
    return () => window.clearInterval(interval);
  }, [lastRefresh]);

  useEffect(() => {
    if (isEditing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [isEditing]);

  const handleRename = async () => {
    const trimmed = editName.trim();
    if (trimmed && trimmed !== account.name) {
      try {
        await onRename(trimmed);
      } catch {
        setEditName(account.name);
      }
    } else {
      setEditName(account.name);
    }
    setIsEditing(false);
  };

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Enter") void handleRename();
    else if (event.key === "Escape") {
      setEditName(account.name);
      setIsEditing(false);
    }
  };

  const livePlanType = account.usage?.error ? null : account.usage?.plan_type?.trim();
  const resolvedPlanType = livePlanType || account.plan_type;
  const planKey =
    resolvedPlanType?.toLowerCase() || (account.auth_mode === "api_key" ? "api_key" : "unknown");
  const planLabel = formatPlanLabel({ ...account, plan_type: resolvedPlanType });
  const chipClass = planChipClass[planKey] ?? planChipClass.free;
  const subscriptionExpiry =
    account.auth_mode === "chat_gpt"
      ? formatSubscriptionExpiry(account.subscription_expires_at)
      : null;
  const switchIsLocked = Boolean(switchDisabled && !restartSwitchEnabled);

  if (isFeatured) {
    return (
      <article className="featured">
        {/* Left column */}
        <div className="featured-head">
          <div className="active-pill">
            <span className="live-dot" />
            {account.is_active ? "Active session" : "Spotlight"}
          </div>

          <div className="acct-identity">
            <div className={`acct-avatar ${account.is_active ? "tinted" : ""}`} aria-hidden="true">
              <PersonIcon className="account-avatar-glyph" />
            </div>
            <div className="acct-meta min-w-0">
              {isEditing ? (
                <input
                  ref={inputRef}
                  type="text"
                  value={editName}
                  onChange={(e) => setEditName(e.target.value)}
                  onBlur={() => void handleRename()}
                  onKeyDown={handleKeyDown}
                  className="field-input"
                  aria-label="Account name"
                />
              ) : (
                <button
                  type="button"
                  onClick={() => {
                    if (!masked) {
                      setEditName(account.name);
                      setIsEditing(true);
                    }
                  }}
                  className="text-left w-full"
                  title={masked ? undefined : "Click to rename"}
                >
                  <div className="acct-name">
                    <PrivateText masked={masked} kind="name">
                      {account.name}
                    </PrivateText>
                  </div>
                </button>
              )}
              {account.email && (
                <div className="acct-email">
                  <PrivateText masked={masked} kind="email">
                    {account.email}
                  </PrivateText>
                </div>
              )}
            </div>
          </div>

          <div className="plan-row">
            <span className={chipClass}>{planLabel}</span>
            {subscriptionExpiry && (
              <span className={`plan-expiry ${subscriptionExpiry.isPast ? "is-past" : ""}`}>
                {subscriptionExpiry.label}
              </span>
            )}
          </div>

          <div className="featured-cta">
            {account.is_active ? (
              <button type="button" disabled className="btn-ghost flex-1">
                Current account
              </button>
            ) : (
              <button
                type="button"
                onClick={onSwitch}
                disabled={switching || switchInProgress || switchIsLocked}
                className="btn-primary flex-1"
              >
                {switching
                  ? "Switching…"
                  : switchDisabled
                    ? restartSwitchEnabled
                      ? "Restart & switch"
                      : "Codex running"
                    : "Switch"}
              </button>
            )}
            <button
              type="button"
              onClick={onDelete}
              className="icon-btn"
              title="Remove account"
              aria-label="Remove account"
            >
              <TrashIcon className="h-4 w-4" />
            </button>
          </div>
        </div>

        {/* Right column — usage */}
        <div className="featured-usage">
          <UsageBar usage={account.usage} loading={account.usageLoading} />
        </div>
      </article>
    );
  }

  return (
    <article className="acct-card">
      <div className="acct-card-top">
        <div className="acct-card-identity min-w-0 flex-1">
          <div className="avatar-sm" aria-hidden="true">
            <PersonIcon className="account-avatar-glyph" />
          </div>
          <div className="min-w-0 flex-1">
            {isEditing ? (
              <input
                ref={inputRef}
                type="text"
                value={editName}
                onChange={(e) => setEditName(e.target.value)}
                onBlur={() => void handleRename()}
                onKeyDown={handleKeyDown}
                className="field-input"
                aria-label="Account name"
              />
            ) : (
              <button
                type="button"
                onClick={() => {
                  if (!masked) {
                    setEditName(account.name);
                    setIsEditing(true);
                  }
                }}
                className="text-left min-w-0 w-full"
                title={masked ? undefined : "Click to rename"}
              >
                <div className="acct-card-name">
                  <PrivateText masked={masked} kind="name">
                    {account.name}
                  </PrivateText>
                </div>
              </button>
            )}
            {account.email && (
              <div className="acct-card-email">
                <PrivateText masked={masked} kind="email">
                  {account.email}
                </PrivateText>
              </div>
            )}
          </div>
        </div>
        <div className="acct-card-plan">
          <span className={chipClass}>{planLabel}</span>
          {subscriptionExpiry && (
            <span className={`plan-expiry ${subscriptionExpiry.isPast ? "is-past" : ""}`}>
              {subscriptionExpiry.label}
            </span>
          )}
        </div>
      </div>

      <div className="usage-block">
        <UsageBar usage={account.usage} loading={account.usageLoading} />
      </div>

      <div className="acct-card-foot">
        <div className="foot-meta">
          <span className="l">Last refresh</span>
          <span className="v">{formatLastRefresh(lastRefresh)}</span>
        </div>
        <button
          type="button"
          onClick={onDelete}
          className="btn-icon"
          title="Remove account"
          aria-label="Remove account"
        >
          <TrashIcon className="h-3.5 w-3.5" />
        </button>
        <button
          type="button"
          onClick={onSwitch}
          disabled={switching || switchInProgress || switchIsLocked}
          className="btn-switch"
          title={
            switchDisabled
              ? restartSwitchEnabled
                ? "Close and reopen Codex after switching"
                : "Enable restart switching or close Codex first"
              : undefined
          }
        >
          {switching
            ? "Switching…"
            : switchDisabled
              ? restartSwitchEnabled
                ? "Restart & switch"
                : "Codex running"
              : "Switch"}
        </button>
      </div>
    </article>
  );
}

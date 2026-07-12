import type { UsageInfo } from "../types";

interface UsageBarProps {
  usage?: UsageInfo;
  loading?: boolean;
}

function formatResetTime(resetAt: number | null | undefined): string {
  if (!resetAt) return "";
  const now = Math.floor(Date.now() / 1000);
  const diff = resetAt - now;
  if (diff <= 0) return "now";
  if (diff < 60) return `${diff}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  return `${Math.floor(diff / 3600)}h ${Math.floor((diff % 3600) / 60)}m`;
}

function formatExactResetTime(resetAt: number | null | undefined): string {
  if (!resetAt) return "";
  const date = new Date(resetAt * 1000);
  const month = new Intl.DateTimeFormat(undefined, { month: "long" }).format(date);
  const day = date.getDate();
  const minutes = String(date.getMinutes()).padStart(2, "0");
  const period = date.getHours() >= 12 ? "PM" : "AM";
  const hour12 = date.getHours() % 12 || 12;
  return `${month} ${day}, ${hour12}:${minutes} ${period}`;
}

function formatWindowDuration(minutes: number | null | undefined): string {
  if (!minutes) return "";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

function formatLimitLabel(
  fallbackLabel: string,
  windowMinutes: number | null | undefined
): string {
  if (!windowMinutes) return fallbackLabel;
  if (windowMinutes === 5 * 60) return "5h limit";
  if (windowMinutes === 7 * 24 * 60) return "Weekly limit";
  return `${formatWindowDuration(windowMinutes)} limit`;
}

function RateLimitBar({
  label,
  usedPercent,
  windowMinutes,
  resetsAt,
}: {
  label: string;
  usedPercent: number;
  windowMinutes?: number | null;
  resetsAt?: number | null;
}) {
  const remainingPercent = Math.max(0, 100 - usedPercent);
  const fillClass =
    remainingPercent <= 20 ? "usage-fill low" :
    remainingPercent <= 40 ? "usage-fill mid" :
    "usage-fill";

  const windowLabel = formatWindowDuration(windowMinutes);
  const displayLabel = formatLimitLabel(label, windowMinutes);
  const resetLabel = formatResetTime(resetsAt);
  const exactResetLabel = formatExactResetTime(resetsAt);

  return (
    <div className="usage-row">
      <div className="usage-row-head">
        <span className="usage-label">
          {displayLabel}{windowLabel ? ` (${windowLabel})` : ""}
        </span>
        <span className="usage-pct">
          {remainingPercent.toFixed(0)}%{" "}
          <span style={{ color: "var(--text-muted)", marginLeft: 2 }}>left</span>
          {resetLabel ? ` · resets ${resetLabel}` : ""}
        </span>
      </div>
      <div className="usage-track">
        <div
          className={fillClass}
          style={{ width: `${Math.min(remainingPercent, 100)}%` }}
        />
      </div>
      {exactResetLabel && (
        <div className="usage-reset">{exactResetLabel}</div>
      )}
    </div>
  );
}

export function UsageBar({ usage, loading }: UsageBarProps) {
  if (loading && !usage) {
    return (
      <div className="usage-row">
        <div className="usage-label animate-pulse" style={{ fontStyle: "italic" }}>
          Fetching usage…
        </div>
        <div className="usage-track">
          <div className="usage-fill animate-pulse" style={{ width: "60%", opacity: 0.3 }} />
        </div>
      </div>
    );
  }

  if (!usage) {
    return (
      <div
        className="usage-reset"
        style={{
          padding: "10px 12px",
          border: "1px dashed var(--hairline)",
          borderRadius: 12,
          fontStyle: "italic",
        }}
      >
        Fetching usage…
      </div>
    );
  }

  if (usage.error) {
    return (
      <div
        style={{
          padding: "10px 12px",
          borderRadius: 12,
          border: "1px solid var(--danger-border)",
          background: "var(--danger-soft)",
          fontSize: 12.5,
          color: "var(--danger)",
          lineHeight: 1.5,
        }}
      >
        {usage.error}
      </div>
    );
  }

  const hasPrimary = usage.primary_used_percent !== null && usage.primary_used_percent !== undefined;
  const hasSecondary = usage.secondary_used_percent !== null && usage.secondary_used_percent !== undefined;

  if (!hasPrimary && !hasSecondary) {
    return (
      <div
        className="usage-reset"
        style={{
          padding: "10px 12px",
          border: "1px dashed var(--hairline)",
          borderRadius: 12,
          fontStyle: "italic",
        }}
      >
        No rate limit data
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {hasPrimary && (
        <RateLimitBar
          label="5h limit"
          usedPercent={usage.primary_used_percent!}
          windowMinutes={usage.primary_window_minutes}
          resetsAt={usage.primary_resets_at}
        />
      )}
      {hasSecondary && (
        <RateLimitBar
          label="Weekly limit"
          usedPercent={usage.secondary_used_percent!}
          windowMinutes={usage.secondary_window_minutes}
          resetsAt={usage.secondary_resets_at}
        />
      )}
      {usage.credits_balance !== null && usage.credits_balance !== undefined && (
        <div className="usage-reset">
          Credits balance:{" "}
          <span style={{ color: "var(--text-muted)" }}>{usage.credits_balance}</span>
        </div>
      )}
    </div>
  );
}

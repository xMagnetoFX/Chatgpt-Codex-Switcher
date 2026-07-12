import type { ReactNode } from "react";
import { CloseIcon, PlusIcon, RefreshIcon, ChevronRightIcon } from "./icons";

export function StatusChip({
  busy = false,
  children,
  className = "",
}: {
  busy?: boolean;
  children: ReactNode;
  className?: string;
}) {
  return (
    <span className={`status-chip ${className}`}>
      <span className={`status-dot ${busy ? "busy" : ""}`} />
      {children}
    </span>
  );
}

export function MetricCard({
  label,
  value,
  note,
  compact = false,
  tone = "default",
}: {
  label: string;
  value: string;
  note: string;
  compact?: boolean;
  tone?: "default" | "nested";
}) {
  void compact; void tone;
  return (
    <div className="metric">
      <div className="metric-label">{label}</div>
      <div className="metric-value">{value}</div>
      <div className="metric-note">{note}</div>
    </div>
  );
}

export function ToolbarActionButton({
  title,
  onClick,
  icon,
  label,
  disabled,
  className = "",
}: {
  title: string;
  onClick: () => void;
  icon: ReactNode;
  label: string;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      aria-label={title}
      className={`icon-btn-lg ${className}`}
    >
      {icon}
      <span className="hidden xl:inline">{label}</span>
    </button>
  );
}

export function SectionHeading({
  eyebrow,
  title,
  description,
}: {
  eyebrow?: string;
  title: string;
  description?: string;
}) {
  return (
    <div>
      {eyebrow && <div className="kicker">{eyebrow}</div>}
      <h3
        style={{
          margin: "4px 0 0",
          fontSize: 17,
          fontWeight: 600,
          letterSpacing: "-0.01em",
          color: "var(--text-strong)",
        }}
      >
        {title}
      </h3>
      {description && (
        <p style={{ margin: "4px 0 0", fontSize: 12.5, color: "var(--text-muted)", lineHeight: 1.5 }}>
          {description}
        </p>
      )}
    </div>
  );
}

export function SettingsPanel({
  title,
  description,
  children,
  className = "",
}: {
  title: string;
  description: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`settings-panel ${className}`}>
      <div className="kicker">Settings</div>
      <h3>{title}</h3>
      <p style={{ margin: "4px 0 16px", fontSize: 12.5, color: "var(--text-muted)", lineHeight: 1.5 }}>
        {description}
      </p>
      <div>{children}</div>
    </section>
  );
}

export function SettingsAction({
  title,
  description,
  icon,
  onClick,
  disabled,
  tone = "default",
  trailing,
  showChevron = true,
}: {
  title: string;
  description: string;
  icon: ReactNode;
  onClick: () => void;
  disabled?: boolean;
  tone?: "default" | "primary";
  trailing?: ReactNode;
  showChevron?: boolean;
}) {
  void tone;
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="action-row"
    >
      <span className="action-icon">{icon}</span>
      <span className="action-meta">
        <strong>{title}</strong>
        <span>{description}</span>
      </span>
      <span className="action-trail">
        {trailing ?? (showChevron ? <ChevronRightIcon className="h-4 w-4" style={{ color: "var(--text-faint)" }} /> : null)}
      </span>
    </button>
  );
}

export function StatePanel({
  eyebrow,
  title,
  description,
  action,
  loading = false,
  tone = "default",
}: {
  eyebrow: string;
  title: string;
  description: string;
  action?: ReactNode;
  loading?: boolean;
  tone?: "default" | "danger";
}) {
  void tone;
  return (
    <div className="glass-surface rounded-[var(--radius-panel)] px-6 py-10 lg:px-8 lg:py-14">
      <div className="mx-auto max-w-2xl text-center">
        <div
          style={{
            width: 64,
            height: 64,
            borderRadius: 22,
            border: `1px solid ${tone === "danger" ? "var(--danger-border)" : "var(--hairline)"}`,
            background: tone === "danger" ? "var(--danger-soft)" : "var(--glass-1)",
            color: tone === "danger" ? "var(--danger)" : "var(--accent)",
            display: "grid",
            placeItems: "center",
            margin: "0 auto",
          }}
        >
          {loading ? (
            <RefreshIcon className="h-7 w-7 animate-spin" />
          ) : tone === "danger" ? (
            <CloseIcon className="h-7 w-7" />
          ) : (
            <PlusIcon className="h-7 w-7" />
          )}
        </div>
        <div className="kicker mt-5">{eyebrow}</div>
        <h3
          style={{
            margin: "6px 0 0",
            fontSize: 22,
            fontWeight: 600,
            letterSpacing: "-0.01em",
            color: "var(--text-strong)",
          }}
        >
          {title}
        </h3>
        <p style={{ margin: "10px 0 0", fontSize: 13.5, color: "var(--text-muted)", lineHeight: 1.6 }}>
          {description}
        </p>
        {action && <div className="mt-6 flex justify-center">{action}</div>}
      </div>
    </div>
  );
}
